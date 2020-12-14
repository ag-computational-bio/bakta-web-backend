package scheduler

import (
	"fmt"

	batchv1 "k8s.io/api/batch/v1"
	v1 "k8s.io/api/core/v1"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
)

func createBaseJobConf(
	id string,
	namespace string,
	downloaderConf string,
	baktaConf string,
	uploaderConf string) *batchv1.Job {
	job := &batchv1.Job{
		ObjectMeta: metav1.ObjectMeta{
			Name:      fmt.Sprintf("bakta-job-%v", id),
			Namespace: namespace,
		},
		Spec: batchv1.JobSpec{
			Template: v1.PodTemplateSpec{
				Spec: v1.PodSpec{
					Containers: []v1.Container{
						v1.Container{
							Name:  "bakta-job",
							Image: "quay.io/mariusdieckmann/bakta-web-job",
							VolumeMounts: []v1.VolumeMount{
								v1.VolumeMount{
									Name:      "database",
									MountPath: "/db",
								},
								v1.VolumeMount{
									Name:      "cache-volume",
									MountPath: "/cache",
								},
							},
							Env: []v1.EnvVar{
								v1.EnvVar{
									Name:  "DownloaderEnvConfig",
									Value: downloaderConf,
								},
								v1.EnvVar{
									Name:  "BaktaEnvConfig",
									Value: downloaderConf,
								},
								v1.EnvVar{
									Name:  "UploaderEnvConfig",
									Value: downloaderConf,
								},
								v1.EnvVar{
									Name: "AWS_ACCESS_KEY_ID",
									ValueFrom: &v1.EnvVarSource{
										SecretKeyRef: &v1.SecretKeySelector{
											Key: "AccessKey",
											LocalObjectReference: v1.LocalObjectReference{
												Name: "s3",
											},
										},
									},
								},
								v1.EnvVar{
									Name: "AWS_SECRET_ACCESS_KEY",
									ValueFrom: &v1.EnvVarSource{
										SecretKeyRef: &v1.SecretKeySelector{
											Key: "SecretKey",
											LocalObjectReference: v1.LocalObjectReference{
												Name: "s3",
											},
										},
									},
								},
							},
						},
					},

					Volumes: []v1.Volume{
						v1.Volume{
							Name: "cache-volume",
							VolumeSource: v1.VolumeSource{
								EmptyDir: &v1.EmptyDirVolumeSource{},
							},
						},
						v1.Volume{
							Name: "database-pb",
							VolumeSource: v1.VolumeSource{
								PersistentVolumeClaim: &v1.PersistentVolumeClaimVolumeSource{
									ClaimName: "bakta-data",
								},
							},
						},
					},
				},
			},
			BackoffLimit: int32Link(4),
		},
	}

	return job
}

func int32Link(value int) *int32 {
	int32Value := int32(value)
	return &int32Value
}
