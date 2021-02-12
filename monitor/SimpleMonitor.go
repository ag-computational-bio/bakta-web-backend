package monitor

import (
	"context"
	"fmt"
	"log"
	"time"

	"github.com/ag-computational-bio/bakta-web-api/go/api"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	v1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/client-go/kubernetes"
)

type SimpleMonitor struct {
	k8sClient *kubernetes.Clientset
	namespace string
}

type JobStatus struct {
	Status   api.JobStatusEnum
	ErrorMsg string
}

func New(k8sClient *kubernetes.Clientset, namespace string) SimpleMonitor {
	return SimpleMonitor{
		k8sClient: k8sClient,
		namespace: namespace,
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
		jobStatus.Status = api.JobStatusEnum_SUCCESFULL
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
