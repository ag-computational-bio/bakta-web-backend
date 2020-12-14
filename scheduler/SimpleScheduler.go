package scheduler

import (
	"context"
	"log"

	"github.com/ag-computational-bio/bakta-web-api/go/api"

	"github.com/ag-computational-bio/bakta-web-backend/database"

	batchv1 "k8s.io/api/batch/v1"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/client-go/kubernetes"
	"k8s.io/client-go/rest"
)

//SimpleScheduler A simple scheduler to run bakta jobs on a Kubernetes cluster
//The config will be picked up from the
type SimpleScheduler struct {
	k8sClient       *kubernetes.Clientset
	databaseHandler *database.Handler
	namespace       string
}

//InitSimpleScheduler Initiates a scheduler to run bakta jobs
func InitSimpleScheduler(namespace string, dbHandler *database.Handler) (*SimpleScheduler, error) {
	config, err := rest.InClusterConfig()
	if err != nil {
		log.Println(err.Error())
		return nil, err
	}

	clientset, err := kubernetes.NewForConfig(config)
	if err != nil {
		log.Println(err.Error())
		return nil, err
	}

	scheduler := SimpleScheduler{
		k8sClient:       clientset,
		databaseHandler: dbHandler,
		namespace:       namespace,
	}

	return &scheduler, nil
}

//StartJob Starts a bakta job on Kubernetes
func (scheduler *SimpleScheduler) StartJob(jobID string, jobConfig *api.JobConfig) (*batchv1.Job, error) {
	job, err := scheduler.databaseHandler.GetJob(jobID)
	if err != nil {
		log.Println(err.Error())
		return nil, err
	}

	downloadConf, err := createDownloadConf(job, false, false)
	if err != nil {
		log.Println(err.Error())
		return nil, err
	}

	baktaConf, err := createBaktaConf(job, jobConfig)
	if err != nil {
		log.Println(err.Error())
		return nil, err
	}

	uploadConf, err := createUploadConf(job)
	if err != nil {
		log.Println(err.Error())
		return nil, err
	}

	apiJob := createBaseJobConf(jobID, scheduler.namespace, downloadConf, baktaConf, uploadConf)
	scheduledJob, err := scheduler.k8sClient.BatchV1().Jobs(scheduler.namespace).Create(context.TODO(), apiJob, metav1.CreateOptions{})
	if err != nil {
		log.Println(err.Error())
		return nil, err
	}

	err = scheduler.databaseHandler.UpdateK8s(jobID, string(scheduledJob.UID))
	if err != nil {
		log.Println(err.Error())
		return nil, err
	}

	return scheduledJob, nil
}
