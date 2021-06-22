package monitor

import (
	"context"
	"fmt"
	"log"
	"math/rand"
	"time"

	api "github.com/ag-computational-bio/bakta-web-api-go/bakta/web/api/proto/v1"
	"github.com/ag-computational-bio/bakta-web-backend/database"
	"github.com/spf13/viper"
	"golang.org/x/sync/errgroup"
	k8serrors "k8s.io/apimachinery/pkg/api/errors"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	v1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/client-go/kubernetes"
)

type SimpleMonitor struct {
	k8sClient kubernetes.Interface
	databaseHandler *database.Handler
	namespace       string
}

type JobStatus struct {
	Status   api.JobStatusEnum
	ErrorMsg string
}

func New(k8sClient kubernetes.Interface, namespace string, db *database.Handler) SimpleMonitor {
	return SimpleMonitor{
		k8sClient:       k8sClient,
		namespace:       namespace,
		databaseHandler: db,
	}
}

func (monitor *SimpleMonitor) GetJobStatus(jobID string) (JobStatus, error) {

	k8sJobName := fmt.Sprintf("bakta-job-%s", jobID)

	time.Sleep(2 * time.Second)

	job, err := monitor.k8sClient.BatchV1().Jobs(monitor.namespace).Get(context.TODO(), k8sJobName, v1.GetOptions{})
	if err != nil {
		log.Println(err.Error())
		return JobStatus{}, err
	}

	jobStatus := JobStatus{
		Status: api.JobStatusEnum_INIT,
	}

	if job.Status.Active >= 1 {
		jobStatus.Status = api.JobStatusEnum_RUNNING
	} else if job.Status.Succeeded >= 1 && job.Status.Active == 0 {
		jobStatus.Status = api.JobStatusEnum_SUCCESSFULL
	} else if job.Status.Failed >= 1 && job.Status.Active == 0 {
		jobStatus.Status = api.JobStatusEnum_ERROR
	} else {
		jobStatus.Status = api.JobStatusEnum_INIT
		errMsg, err := monitor.getJobPodError(jobID)
		if err != nil {
			log.Println(err.Error())
		}
		jobStatus.ErrorMsg = errMsg
	}

	return jobStatus, nil
}

func (monitor *SimpleMonitor) getJobPodError(jobID string) (string, error) {
	labelSelector := fmt.Sprintf("job-name=%s%s", "bakta-job-", jobID)

	listOptions := metav1.ListOptions{
		LabelSelector: labelSelector,
	}

	pods, err := monitor.k8sClient.CoreV1().Pods(monitor.namespace).List(context.TODO(), listOptions)
	if err != nil {
		log.Println(err.Error())
		return "", err
	}

	if len(pods.Items) == 0 {
		err := fmt.Errorf("Could not find pods for job %v", jobID)
		log.Println(err.Error())
		return "", err
	}

	pod := pods.Items[len(pods.Items)-1]

	return pod.Status.Message, nil
}

func (monitor *SimpleMonitor) RunFindStragglersLoop() {
	go func() {
		stragglerWaitTime := viper.GetInt("StragglerWaitTime")

		initialOffset := rand.Intn(stragglerWaitTime)
		time.Sleep(time.Duration(initialOffset) * time.Minute)

		for {
			log.Println("starting cleanup cycle")
			monitor.findRunningStragglers()
			log.Println("finished cleanup cycle")
			time.Sleep(time.Duration(stragglerWaitTime) * time.Minute)
		}
	}()
}

// findRunningStragglers Checks all jobs in the database with running status if they have an correponding
// job running in the Kubernetes cluster
func (monitor *SimpleMonitor) findRunningStragglers() error {
	jobs, err := monitor.databaseHandler.GetRunningJobs()
	if err != nil {
		log.Println(err.Error())
		return err
	}

	running_checker_errgp := errgroup.Group{}
	jobs_channel := make(chan *database.Job, 500)

	for i := 1; i <= 100; i++ {
		running_checker_errgp.Go(func() error {
			return monitor.checkAndHandleRunningStraggler(jobs_channel)
		})
	}

	go func() {
		defer close(jobs_channel)
		for _, job := range jobs {
			jobs_channel <- job
		}
	}()

	err = running_checker_errgp.Wait()
	if err != nil {
		log.Println(err.Error())
		return err
	}

	return nil
}

func (monitor *SimpleMonitor) checkAndHandleRunningStraggler(jobs chan *database.Job) error {
	for job := range jobs {
		_, err := monitor.GetJobStatus(job.JobID)
		if !k8serrors.IsNotFound(err) {
			if err != nil {
				log.Println(err.Error())
			}
			return err
		}

		// Check again if job is still running in database to avoid consistency problems with jobs that finish between the initial query and the k8s check
		updatedJob, err := monitor.databaseHandler.GetJob(job.JobID)
		if err != nil {
			log.Println(err.Error())
		}

		if updatedJob.Status == api.JobStatusEnum_RUNNING.String() && !updatedJob.IsDeleted {
			err := monitor.databaseHandler.UpdateStatus(job.JobID, api.JobStatusEnum_ERROR, "job was in running state but no running k8s job could be found", true)
			if err != nil {
				log.Println(err.Error())
				return err
			}
		}

	}

	return nil
}
